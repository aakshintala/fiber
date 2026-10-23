//! The process boundary in a session's log (`docs/events.md`, "Process
//! boundary"): `fiber_started` before the session's first line, and
//! `fiber_exited`, from the loop's `Summary`, after the loop has stopped.
//! Only the loop writes durable events (`docs/architecture.md`, "Streaming").

use contract::ActionId;
use contract::RequestId;
use contract::events::{
    Answer, Event, ExtensionsLoaded, FiberExited, FiberStarted, FinalMessage, InteractionResolved,
    LoadedExtension, McpServerFailed, MessageOutcome, Notice, ResolvedBy, TurnOutcome,
};
use contract::shapes::{Failure, Question, True};
use log::Log;
use serde_json::Value;

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

/// What `Loop::run` returns: the run's `Summary` beside its result, so
/// `fiber_exited` reports this process's lines without reading the log.
/// A failed run still carries its summary: usage written before the
/// failure is still reported (`a_failed_run_exits_1_with_its_error`).
#[derive(Debug)]
pub struct Ran {
    /// This process's lines, folded as they were written.
    pub summary: Summary,
    /// How the run ended.
    pub result: Result<(), Error>,
}

/// What `fiber_exited` reports: this process's final message, usage, open
/// requests and turn error, folded from each line as the loop wrote it.
/// Each process builds its own, starting empty; `fiber_exited` reads
/// nothing back from the log (`docs/events.md`, `fiber_exited`: "That copy
/// is output, never a source").
#[derive(Debug, Default, Clone)]
pub struct Summary {
    /// This process's model calls, latest per generation: `fiber_exited`
    /// reports them as `usage` (`docs/events.md`, `fiber_exited`).
    ledger: Ledger,
    final_message: Option<FinalMessage>,
    error: Option<Failure>,
    questions: Option<Vec<Question>>,
    /// Text parts of the open assistant message, joined in log order.
    /// Every message takes them: messages never interleave, and a failed
    /// call logs none, so the next message must not inherit them.
    text: String,
    /// Requests this process raised and has not resolved, oldest first.
    /// Each entry also says whether an earlier process left it open.
    open: Vec<(RequestId, bool)>,
    /// The extension asks still unresolved, oldest first. A resumed
    /// session starts a new VM, so no callback waits for the answer and
    /// nothing can raise the question again (`docs/events.md`,
    /// `interaction_resolved`).
    open_extension: Vec<RequestId>,
    /// Repository offers raised: `preamble_built` ends the wait on each.
    offers: Vec<RequestId>,
    /// Whether `rewound` was written: it closes the process boundary as
    /// `fiber_exited` does (`docs/events.md`, "Rewind").
    rewound: bool,
}

impl Summary {
    /// Folds one written event into the summary, with the line's
    /// `action_id`. Runs only after a successful append, so every folded
    /// line is on the log.
    pub(crate) fn observe(&mut self, event: &Event, action: Option<&ActionId>) {
        if let Event::PermissionRequested(requested) = event {
            self.raised(requested.request_id.clone(), false);
        } else if let Event::InteractionRequested(requested) = event {
            self.raised(requested.request_id.clone(), requested.extension.is_some());
        } else if let Event::PermissionResolved(resolved) = event {
            if let Some(id) = &resolved.request_id {
                self.open.retain(|pending| &pending.0 != id);
            }
        } else if let Event::InteractionResolved(resolved) = event {
            self.answered(&resolved.request_id);
        } else if let Event::RepositoryCodeOffered(offered) = event {
            // An offer precedes every request its process raises, so it goes
            // first: `suspended_on` names an approval or question whenever
            // one is open too, and resume finds the suspended turn by it.
            self.open.insert(0, (offered.request_id.clone(), false));
            self.offers.push(offered.request_id.clone());
        } else if let Event::RepositoryCodeResolved(resolved) = event {
            self.open.retain(|pending| pending.0 != resolved.request_id);
        } else if let Event::PreambleBuilt(_) = event {
            // A process that built its preamble no longer waits on an offer.
            self.open
                .retain(|pending| !self.offers.contains(&pending.0));
        } else if let Event::Rewound(_) = event {
            self.rewound = true;
        }
        if let Event::TurnStarted(_) = event {
            self.final_message = None;
            self.error = None;
            self.questions = None;
        } else if let Event::TextCompleted(part) = event {
            self.text.push_str(&part.text);
        } else if let Event::AssistantMessageCompleted(message) = event {
            let part = std::mem::take(&mut self.text);
            if message.outcome == MessageOutcome::Completed
                && let Some(id) = action
            {
                self.final_message = Some(FinalMessage {
                    final_action_id: id.clone(),
                    text: part,
                });
            }
        } else if let Event::TurnCompleted(turn) = event {
            if turn.outcome == TurnOutcome::Failed {
                self.final_message = None;
            }
            self.error = turn.error.clone();
            self.questions = turn.questions.clone();
        } else if let Event::UsageRecorded(recorded) = event {
            self.ledger.record(recorded);
        }
    }

    /// Marks every request open now as left by an earlier process: it is
    /// named `suspended_on` only under a signal. Extension asks need no
    /// mark: they are declined whichever process raised them.
    pub(crate) fn carry(&mut self) {
        for pending in &mut self.open {
            pending.1 = true;
        }
    }

    /// Folds a line of an earlier process by its raw `payload`, for the
    /// kinds the resume history pass would otherwise never parse: a
    /// `request_id` that is not a string skips the line, as a payload that
    /// does not read contributes nothing to the exit.
    pub(crate) fn seed_raw(&mut self, kind: &str, payload: &serde_json::Map<String, Value>) {
        let Some(id) = payload.get("request_id").and_then(Value::as_str) else {
            return;
        };
        let id = RequestId(id.to_owned());
        match kind {
            "permission_requested" => self.raised(id, false),
            "interaction_requested" => {
                self.raised(id, payload.get("extension").is_some_and(Value::is_string));
            }
            "interaction_resolved" => self.answered(&id),
            _ => {}
        }
    }

    /// A request raised. An extension question belongs to a callback in
    /// this process's VM, so resuming never raises it again: it is
    /// declined at exit, not suspended on
    /// (`an_open_extension_ask_is_declined_at_exit`).
    fn raised(&mut self, id: RequestId, extension: bool) {
        if extension {
            self.open_extension.push(id);
        } else {
            self.open.push((id, false));
        }
    }

    /// An interaction resolved. Whoever answered an extension ask already
    /// wrote the one resolution (`a_resolved_extension_ask_is_left_alone`).
    fn answered(&mut self, id: &RequestId) {
        self.open.retain(|pending| &pending.0 != id);
        self.open_extension.retain(|pending| pending != id);
    }

    /// The latest approval or question this process left unresolved, or
    /// else its unresolved repository offer. Extension asks are never here;
    /// they are declined at exit instead. A request an earlier process
    /// left open counts only under a signal (`docs/invocation.md`,
    /// "Shutdown").
    fn suspended_on(&self, signalled: bool) -> Option<RequestId> {
        self.open
            .iter()
            .rev()
            .find(|(_, carried)| signalled || !carried)
            .map(|(id, _)| id.clone())
    }
}

/// Writes `fiber_exited` once the loop has stopped: the final message and
/// the usage from this process's `summary`, and `ran`'s error or else, for
/// a `one_turn` run (`fiber ask`), the last turn's. A session the hub
/// started does not fail when a turn did: its clients saw each
/// `turn_completed`. Returns what it wrote: exit code 1 with an `error`
/// (`docs/errors.md`, "What a caller gets"), otherwise 0. Under a
/// shutdown, `signal` is its exit code: it is the code written, with no
/// `error` and no final message, and `suspended_on` also names a request an
/// earlier process left unresolved (`docs/invocation.md`, "Shutdown").
pub fn fiber_exited(
    log: &Log,
    summary: &Summary,
    ran: Result<(), Failure>,
    one_turn: bool,
    signal: Option<i32>,
) -> Result<Exited, Error> {
    // A rewound session's last line is `rewound`, which closes the process
    // boundary as `fiber_exited` does: nothing more is written, and open
    // extension asks are left as they are (`docs/events.md`, "Rewind").
    if summary.rewound {
        let error = ran.err();
        return Ok(Exited {
            code: i32::from(error.is_some()),
            error,
        });
    }
    let turn_error = if one_turn {
        summary.error.clone()
    } else {
        None
    };
    let error = ran.err().or(turn_error);
    let (exit_code, error, final_message) = match signal {
        Some(code) => (code, None, None),
        None => (
            i32::from(error.is_some()),
            error,
            summary.final_message.clone(),
        ),
    };
    decline_extension_asks(log, &summary.open_extension)?;
    let exited = Event::FiberExited(FiberExited {
        exit_code,
        usage: summary.ledger.usage(),
        final_message,
        error: error.clone(),
        suspended_on: summary.suspended_on(signal.is_some()),
        questions: summary.questions.clone(),
    });
    log.append(&exited, None, None)?;
    Ok(Exited {
        code: exit_code,
        error,
    })
}

/// Declines every still-open extension ask with `by: fiber`, in log order,
/// before `fiber_exited`'s own line. A resumed session's status never waits
/// on a question no callback holds.
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

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
