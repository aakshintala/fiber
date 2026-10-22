//! The loop's small shared helpers: random ids, the session environment,
//! and the one path every emitted event takes to the log and the
//! conversation.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use contract::events::{
    Class, Event, Outcome, TurnCompleted, TurnOutcome, Variables, VariablesSource,
};
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

pub(crate) use support::mint;

/// Where this process's environment came from; first call wins. Set once, by the session command.
static SOURCE: OnceLock<VariablesSource> = OnceLock::new();

/// Where this process's environment came from; first call wins. Set once, by the session command.
pub fn set_variables_source(source: VariablesSource) {
    let _ignored: Result<(), VariablesSource> = SOURCE.set(source);
}

/// `Inherited` until set.
pub fn variables_source() -> VariablesSource {
    SOURCE.get().copied().unwrap_or(VariablesSource::Inherited)
}

/// This process's environment, as `session_started` records it: the `PATH`
/// and the other names, never a value (`docs/invocation.md`, "A session's
/// environment").
pub(crate) fn variables() -> Variables {
    Variables::of(std::env::vars_os(), variables_source())
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
    /// The session's known network hosts, folded from the same durable events.
    pub(crate) known: &'a mut crate::known_hosts::KnownHosts,
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
            self.known.fold(event);
            // A written `shell_command` waits for the next turn under its
            // line's `seq`: the `turn_started` naming it releases it.
            if let Event::ShellCommand(shell) = event
                && let Some(seq) = line.seq.map(|seq| seq.0)
            {
                self.carry.hold(seq, shell);
            }
            let was_summary = self.carry.in_summary();
            crate::conversation::render(
                self.conversation,
                event,
                action,
                model,
                self.had,
                self.carry,
                line.seq,
            );
            if !was_summary && !self.carry.in_summary() {
                crate::reviewer::render_reviewed(self.reviewed, event, action, line.seq);
            }
        }
        Ok(())
    }
}

impl crate::Loop {
    /// Writes `event` to the log and renders it into the conversation and
    /// the reviewer's transcript, through the one [`Transcript`] path.
    ///
    /// Public for the `review` jig's replay, which renders a source log's
    /// handoff boundary through the same path (`docs/testing.md`,
    /// "Jigs"); the jig never writes to the source directory.
    pub fn write(
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
            known: &mut self.known_hosts,
        }
        .write(&self.log, &self.model.reference, event, turn, action)?;
        if event.class() == Class::Durable {
            self.ending.starts.fold(event);
        }
        // A completed handoff restarts the reviewer's input from the kept
        // person's messages (`docs/permissions.md`, "At a handoff"), so
        // the selection that follows re-sends from the start instead of
        // reusing a prefix end that names the old input. A failed or
        // cancelled handoff, or a refused append, changes nothing.
        if matches!(
            event,
            Event::HandoffCompleted(done) if done.outcome == Outcome::Completed
        ) {
            self.reviewer_sent = None;
        }
        Ok(())
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

#[cfg(test)]
#[path = "util_tests.rs"]
mod tests;
