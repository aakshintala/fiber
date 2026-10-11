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

/// The render sinks every durable line is written into.
pub(crate) struct Transcript<'a> {
    /// The conversation, built from the durable events as they are written.
    pub(crate) conversation: &'a mut Vec<Input>,
    /// What the reviewer is shown, rendered from the same events.
    pub(crate) reviewed: &'a mut Vec<crate::reviewer::Reviewed>,
    /// The content the model last had per instruction file path.
    pub(crate) had: &'a mut BTreeMap<String, String>,
    /// The render state a handoff reads.
    pub(crate) carry: &'a mut crate::handoff::Carry,
}

impl Transcript<'_> {
    /// Writes `event` to `log` and renders it into the sinks: the one path
    /// every event the loop emits takes, so the conversation is the log's
    /// rendering (`docs/loop.md`, "What the model is sent") and the
    /// reviewer's transcript its projection (`docs/permissions.md`, "What
    /// it is shown"). A non-durable event is appended and renders nothing;
    /// an append error returns before any render.
    pub(crate) fn write(
        &mut self,
        log: &Log,
        model: &str,
        event: &Event,
        turn: Option<&TurnId>,
        action: Option<&ActionId>,
    ) -> Result<(), Error> {
        let line = log.append(event, turn.cloned(), action.cloned())?;
        if event.class() == Class::Durable {
            crate::conversation::render(
                self.conversation,
                event,
                action,
                model,
                self.had,
                self.carry,
            );
            crate::reviewer::render_reviewed(self.reviewed, event, action, line.seq);
        }
        Ok(())
    }
}

impl crate::Loop {
    /// Writes `event` to the log and renders it into the conversation and
    /// the reviewer's transcript, through the one [`Transcript`] path.
    pub(crate) fn write(
        &mut self,
        event: &Event,
        turn: Option<&TurnId>,
        action: Option<&ActionId>,
    ) -> Result<(), crate::Error> {
        Transcript {
            conversation: &mut self.conversation,
            reviewed: &mut self.reviewed,
            had: &mut self.changes.had,
            carry: &mut self.handoff.carry,
        }
        .write(&self.log, &self.model.reference, event, turn, action)
    }

    /// Writes `event` and renders it into the conversation.
    pub(crate) fn append(
        &mut self,
        event: &Event,
        turn: &TurnId,
        action: Option<&ActionId>,
    ) -> Result<(), crate::Error> {
        self.write(event, Some(turn), action)
    }
}
