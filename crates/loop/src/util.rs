//! The loop's small shared helpers: random ids, the session environment,
//! and the one path every emitted event takes to the log and the
//! conversation.

use std::collections::{BTreeMap, hash_map::RandomState};
use std::hash::BuildHasher;

use contract::events::{Class, Event, TurnCompleted, TurnOutcome, Variables, VariablesSource};
use contract::provider::Input;
use contract::shapes::Failure;
use contract::{ActionId, TurnId};
use log::Log;

use crate::Error;

/// `turn_completed` with `outcome`, and `error` on `failed`.
pub(crate) fn ended(outcome: TurnOutcome, error: Option<Failure>) -> TurnCompleted {
    TurnCompleted {
        outcome,
        error,
        questions: None,
    }
}

/// A new id from random bytes (`docs/events.md`, "Identity and ordering").
/// `RandomState` seeds its keys from the operating system's randomness.
pub(crate) fn mint(prefix: &str) -> String {
    format!("{prefix}{:016x}", RandomState::new().hash_one(()))
}

/// This process's environment, as `session_started` records it: the `PATH`
/// and the other names, never a value. No hub starts a session yet, so the
/// source is always `inherited` (`docs/invocation.md`, "A session's
/// environment").
pub(crate) fn variables() -> Variables {
    let mut path = String::new();
    let mut names = Vec::new();
    for (name, value) in std::env::vars_os() {
        if name == "PATH" {
            path = value.to_string_lossy().into_owned();
        } else {
            names.push(name.to_string_lossy().into_owned());
        }
    }
    names.sort();
    Variables {
        path,
        names,
        source: VariablesSource::Inherited,
    }
}

/// Writes `event` to `log` and renders it into `conversation` and
/// `reviewed`: the one path every event the loop emits takes, so the
/// conversation is the log's rendering (`docs/loop.md`, "What the model is
/// sent") and the reviewer's transcript its projection
/// (`docs/permissions.md`, "What it is shown"). `had` is the content the
/// model last had per instruction file path, updated as instruction lines
/// render. `carry` is the render state a handoff reads.
#[allow(
    clippy::too_many_arguments,
    reason = "the one write path takes every sink it renders into"
)]
pub(crate) fn write(
    log: &Log,
    conversation: &mut Vec<Input>,
    reviewed: &mut Vec<crate::reviewer::Reviewed>,
    model: &str,
    event: &Event,
    turn: Option<&TurnId>,
    action: Option<&ActionId>,
    had: &mut BTreeMap<String, String>,
    carry: &mut crate::handoff::Carry,
) -> Result<(), Error> {
    let line = log.append(event, turn.cloned(), action.cloned())?;
    if event.class() == Class::Durable {
        crate::conversation::render(conversation, event, action, model, had, carry);
        crate::reviewer::render_reviewed(reviewed, event, action, line.seq);
    }
    Ok(())
}

impl crate::Loop {
    /// Writes `event` and renders it into the conversation.
    pub(crate) fn append(
        &mut self,
        event: &Event,
        turn: &TurnId,
        action: Option<&ActionId>,
    ) -> Result<(), crate::Error> {
        write(
            &self.log,
            &mut self.conversation,
            &mut self.reviewed,
            &self.model.reference,
            event,
            Some(turn),
            action,
            &mut self.changes.had,
            &mut self.handoff.carry,
        )
    }
}
