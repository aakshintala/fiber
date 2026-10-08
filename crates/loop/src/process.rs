//! The process boundary in a session's log (`docs/events.md`, "Process
//! boundary"): `fiber_started` before the session's first line, and
//! `fiber_exited`, folded from the log, after the loop has stopped. Only the
//! loop writes durable events (`docs/architecture.md`, "Streaming").

use std::path::Path;

use contract::RequestId;
use contract::events::{
    Answer, Event, ExtensionsLoaded, FiberExited, FiberStarted, FinalMessage, InteractionResolved,
    LoadedExtension, McpServerFailed, MessageOutcome, Notice, ResolvedBy, TurnOutcome,
};
use contract::shapes::{Failure, Question, True};
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
    // A rewound session's last line is `rewound`, which closes the process
    // boundary as `fiber_exited` does: nothing more is written, and open
    // extension asks are left as they are (`docs/events.md`, "Rewind").
    if log::last_line(dir).is_some_and(|line| line.kind == "rewound") {
        let error = ran.err();
        return Ok(Exited {
            code: i32::from(error.is_some()),
            error,
        });
    }
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
        retry_after_ms: None,
        provider: None,
    });
    let turn_error = if one_turn { fold.error } else { None };
    let error = ran.err().or(unread).or(turn_error);
    let (exit_code, error, final_message) = match signal {
        Some(code) => (code, None, None),
        None => (i32::from(error.is_some()), error, fold.final_message),
    };
    decline_extension_asks(log, &fold.open_extension)?;
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
    /// The latest approval or question this process left unresolved, or
    /// else its unresolved repository offer. Extension asks are never here; they are declined at exit instead.
    suspended_on: Option<RequestId>,
    /// The extension asks still unresolved, oldest first. Unlike `open`
    /// below, these survive `fiber_started`: a resumed session starts a new
    /// VM, so no callback waits for the answer and nothing can raise the
    /// question again (`docs/events.md`, `interaction_resolved`).
    open_extension: Vec<RequestId>,
}

/// Declines every still-open extension ask with `by: fiber`, in log order,
/// before `fiber_exited`'s own line. A resumed session's status never waits
/// on a question no callback holds. A fold that failed to read leaves the
/// default fold, so nothing is declined then.
fn decline_extension_asks(log: &Log, open: &[RequestId]) -> Result<(), Error> {
    for request_id in open {
        let resolved = Event::InteractionResolved(InteractionResolved {
            request_id: request_id.clone(),
            by: ResolvedBy::Fiber,
            answer: Answer::Declined { declined: True },
        });
        log.append(&resolved, None, None)?;
    }
    Ok(())
}

/// Folds `lines`, one at a time. With `keep_open`, the requests raised
/// before the latest `fiber_started` and still unresolved stay open across
/// it. A line whose payload does not read contributes nothing; the first
/// such line after the latest `fiber_started` fails the fold with its
/// error, and one before it belongs to an earlier process and does not. A
/// line that is not an envelope ends the fold with its error.
fn fold(lines: log::Lines, keep_open: bool) -> Result<Fold, Error> {
    let mut fold = Fold::default();
    // Text parts of the open assistant message, joined in log order.
    // Every message takes them: messages never interleave, and a failed
    // call logs none, so the next message must not inherit them.
    let mut text = String::new();
    // Requests this process raised and has not resolved, oldest first.
    // `suspended_on` is the latest (`docs/events.md`, `fiber_exited`).
    let mut open = Vec::new();
    // Extension asks still unresolved, oldest first. These survive
    // `fiber_started`: an earlier process's ask, such as after a crash,
    // is declined too (an_ask_an_earlier_process_left_open_is_declined).
    let mut open_extension = Vec::new();
    // Repository offers raised: `preamble_built` ends the wait on each.
    let mut offers = Vec::new();
    // The first payload since the latest `fiber_started` that did not read.
    let mut unread = None;
    for line in lines {
        let line = line?;
        let event = match Event::from_envelope(&line) {
            Ok(event) => event,
            Err(e) => {
                unread.get_or_insert(Error::Unreadable(e));
                continue;
            }
        };
        // Each process reports its own lines only: the fold restarts at
        // the latest `fiber_started` (`docs/events.md`, `fiber_exited`:
        // `usage` is "this process's model calls for the session").
        if matches!(&event, Some(Event::FiberStarted(_))) {
            fold = Fold::default();
            text.clear();
            unread = None;
            if !keep_open {
                open.clear();
            }
            continue;
        }
        if let Some(Event::PermissionRequested(requested)) = &event {
            open.push(requested.request_id.clone());
        } else if let Some(Event::InteractionRequested(requested)) = &event {
            // an_open_extension_ask_is_declined_at_exit, not suspended: an
            // extension question belongs to a callback in this process's
            // VM, so resuming never raises it again.
            if requested.extension.is_some() {
                open_extension.push(requested.request_id.clone());
            } else {
                open.push(requested.request_id.clone());
            }
        } else if let Some(Event::PermissionResolved(resolved)) = &event {
            if let Some(id) = &resolved.request_id {
                open.retain(|pending| pending != id);
            }
        } else if let Some(Event::InteractionResolved(resolved)) = &event {
            // a_resolved_extension_ask_is_left_alone: whoever answered it
            // already wrote the one resolution.
            open.retain(|pending| pending != &resolved.request_id);
            open_extension.retain(|pending| pending != &resolved.request_id);
        } else if let Some(Event::RepositoryCodeOffered(offered)) = &event {
            // An offer precedes every request its process raises, so it goes
            // first: `suspended_on` names an approval or question whenever
            // one is open too, and resume finds the suspended turn by it.
            open.insert(0, offered.request_id.clone());
            offers.push(offered.request_id.clone());
        } else if let Some(Event::RepositoryCodeResolved(resolved)) = &event {
            open.retain(|pending| pending != &resolved.request_id);
        } else if let Some(Event::PreambleBuilt(_)) = &event {
            // A process that built its preamble no longer waits on an offer.
            open.retain(|pending| !offers.contains(pending));
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
    if let Some(e) = unread {
        return Err(e);
    }
    fold.suspended_on = open.last().cloned();
    fold.open_extension = open_extension;
    Ok(fold)
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
