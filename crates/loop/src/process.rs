//! The process boundary in a session's log (`docs/events.md`, "Process
//! boundary"): `fiber_started` before the session's first line, and
//! `fiber_exited`, folded from the log, after the loop has stopped. Only the
//! loop writes durable events (`docs/architecture.md`, "Streaming").

use std::collections::BTreeMap;
use std::path::Path;

use contract::ActionId;
use contract::Envelope;
use contract::events::{
    Event, FiberExited, FiberStarted, FinalMessage, MessageOutcome, TurnOutcome,
};
use contract::shapes::{Failure, Question, Tokens, Usage};
use log::Log;

use crate::Error;

/// Writes `fiber_started` for a new session, the first line this process
/// writes for it, naming the running Fiber's `version`.
pub fn fiber_started(log: &Log, version: &str) -> Result<(), Error> {
    let started = Event::FiberStarted(FiberStarted {
        version: version.to_owned(),
        resumed: false,
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
        usage: fold.usage.into(),
        final_message: fold.final_message,
        error,
        suspended_on: None,
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
    usage: Totals,
}

/// Usage totals while folding: the tokens, how many calls were billed per
/// token, and their known costs.
struct Totals {
    tokens: Tokens,
    billed: usize,
    cost: Option<f64>,
    subscription_cost: f64,
}

impl Default for Totals {
    fn default() -> Self {
        Self {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            billed: 0,
            cost: None,
            subscription_cost: 0.0,
        }
    }
}

impl From<Totals> for Usage {
    fn from(t: Totals) -> Self {
        Self {
            tokens: t.tokens,
            // `docs/events.md`, "usage": 0 with no billed call, null when no
            // billed call had a known cost.
            cost: if t.billed == 0 { Some(0.0) } else { t.cost },
            subscription_cost: t.subscription_cost,
        }
    }
}

fn fold(lines: &[Envelope]) -> Result<Fold, serde_json::Error> {
    let mut fold = Fold::default();
    // Text parts of each assistant message, joined in log order.
    let mut parts: BTreeMap<ActionId, String> = BTreeMap::new();
    for line in lines {
        let event = Event::from_envelope(line)?;
        if let Some(Event::TurnStarted(_)) = &event {
            fold.final_message = None;
            fold.error = None;
            fold.questions = None;
        } else if let Some(Event::TextCompleted(part)) = &event
            && let Some(id) = &line.action_id
        {
            parts.entry(id.clone()).or_default().push_str(&part.text);
        } else if let Some(Event::AssistantMessageCompleted(message)) = &event
            && message.outcome == MessageOutcome::Completed
            && let Some(id) = &line.action_id
        {
            fold.final_message = Some(FinalMessage {
                final_action_id: id.clone(),
                text: parts.get(id).cloned().unwrap_or_default(),
            });
        } else if let Some(Event::TurnCompleted(turn)) = &event {
            if turn.outcome == TurnOutcome::Failed {
                fold.final_message = None;
            }
            fold.error = turn.error.clone();
            fold.questions = turn.questions.clone();
        } else if let Some(Event::UsageRecorded(call)) = &event {
            let usage = &mut fold.usage;
            let tokens = &mut usage.tokens;
            tokens.input += call.tokens.input;
            tokens.cache_read += call.tokens.cache_read;
            tokens.output += call.tokens.output;
            for (lifetime, n) in &call.tokens.cache_write {
                *tokens.cache_write.entry(lifetime.clone()).or_default() += n;
            }
            if call.subscription == Some(true) {
                usage.subscription_cost += call.cost.unwrap_or(0.0);
            } else {
                usage.billed += 1;
                if let Some(cost) = call.cost {
                    *usage.cost.get_or_insert(0.0) += cost;
                }
            }
        }
    }
    Ok(fold)
}
