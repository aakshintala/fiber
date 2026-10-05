//! The process boundary in a session's log (`docs/events.md`, "Process
//! boundary"): `fiber_started` before the session's first line, and
//! `fiber_exited`, folded from the log, after the loop has stopped. Only the
//! loop writes durable events (`docs/architecture.md`, "Streaming").

use std::path::Path;

use contract::events::{
    Event, FiberExited, FiberStarted, FinalMessage, MessageOutcome, TurnOutcome,
};
use contract::shapes::{Failure, Question};
use contract::{Envelope, RequestId};
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

/// Writes `fiber_exited` once the loop has stopped: the final message and
/// the usage, folded from the session's log in `dir`, and `ran`'s error or
/// else the last turn's. Returns the exit code it wrote: 1 when either
/// failed (`docs/errors.md`, "What a caller gets"), otherwise 0.
pub fn fiber_exited(log: &Log, dir: &Path, ran: Result<(), Failure>) -> Result<i32, Error> {
    let folded = log::read(dir)
        .map_err(Error::from)
        .and_then(|lines| fold(&lines).map_err(Error::Unreadable));
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
    let error = ran.err().or(unread).or(fold.error);
    let exit_code = i32::from(error.is_some());
    let exited = Event::FiberExited(FiberExited {
        exit_code,
        usage: fold.ledger.usage(),
        final_message: fold.final_message,
        error,
        suspended_on: fold.suspended_on,
        questions: fold.questions,
    });
    log.append(&exited, None, None)?;
    Ok(exit_code)
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

fn fold(lines: &[Envelope]) -> Result<Fold, serde_json::Error> {
    let mut fold = Fold::default();
    // Text parts of the open assistant message, joined in log order.
    // Every message takes them: messages never interleave, and a failed
    // call logs none, so the next message must not inherit them.
    let mut text = String::new();
    // Requests this process raised and has not resolved, oldest first.
    // `suspended_on` is the latest (`docs/events.md`, `fiber_exited`).
    let mut open = Vec::new();
    for line in lines {
        let event = Event::from_envelope(line)?;
        // Each process reports its own lines only: the fold restarts at
        // the latest `fiber_started` (`docs/events.md`, `fiber_exited`:
        // `usage` is "this process's model calls for the session").
        if matches!(&event, Some(Event::FiberStarted(_))) {
            fold = Fold::default();
            text.clear();
            open.clear();
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
