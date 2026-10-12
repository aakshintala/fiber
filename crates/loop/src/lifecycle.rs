//! The `turn_start`, `before_model_call` and `turn_end` call sites
//! (`docs/extensions.md`, "The hook points"): what each hook is shown,
//! and how its answer reaches the turn.

use contract::events::{ContextAdded, Event, InputItem, TurnCompleted, TurnOutcome};
use contract::hook::{ContextOutcome, TurnStart};
use contract::shapes::Failure;
use contract::{ErrorCode, TurnId};

use crate::{Error, Loop};

/// What `turn_end` hooks returned and the next step writes, and what a
/// resumed turn's `session_start` contexts wait in (Part 3): held across
/// calls, never in `queued`, so a cancel never carries them into another
/// turn.
#[derive(Default)]
pub(crate) struct State {
    /// Messages `turn_end` hooks continued the turn with, oldest first.
    pub(crate) continued: Vec<contract::hook::Context>,
}

impl Loop {
    /// Runs the session's `turn_start` hooks on the turn's input, in order,
    /// after its acknowledgements and before its first model request
    /// (`docs/loop.md`, "Starting a turn"). `Some` ends the turn through
    /// `end_turn`; `None` runs its steps.
    pub(crate) fn turn_start(
        &mut self,
        turn: &TurnId,
        input: &[InputItem],
    ) -> Result<Option<TurnCompleted>, Error> {
        // A cancel or a shutdown runs no hook: the turn ends `interrupted`
        // instead.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        let Some(hooks) = self.hooks.clone() else {
            return Ok(None);
        };
        // Items this build does not know carry nothing a hook could read.
        let shown: Vec<InputItem> = input
            .iter()
            .filter(|item| !matches!(item, InputItem::Unknown))
            .cloned()
            .collect();
        let answer = hooks.turn_start(&TurnStart {
            input: &shown,
            cancel: &*self.cancel,
        });
        let contract::hook::ContextAnswer { outcome, notices } = answer;
        for notice in notices {
            self.append(&Event::Notice(notice), turn, None)?;
        }
        // A cancel that landed while the hooks ran applies none of the
        // answer: only the notices above are written.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        match outcome {
            ContextOutcome::Added(contexts) => {
                for added in contexts {
                    self.append(
                        &Event::ContextAdded(ContextAdded {
                            text: added.text,
                            extension: added.extension,
                            hook: "turn_start".to_owned(),
                        }),
                        turn,
                        None,
                    )?;
                }
                Ok(None)
            }
            ContextOutcome::Failed(failure) => Ok(Some(crate::ended(
                TurnOutcome::Failed,
                Some(Failure {
                    code: ErrorCode::HookFailed,
                    message: format!(
                        "The `turn_start` hook of extension {} failed, \
                         so the turn did not run: {}",
                        failure.extension, failure.message
                    ),
                    retry_after_ms: None,
                    provider: None,
                }),
            ))),
        }
    }

    /// Runs the session's `before_model_call` hooks on the step's built
    /// request, once the budget allows it and before it is sent
    /// (`docs/loop.md`, "Spending budget"). `Some` ends the step without
    /// sending; `None` sends the request.
    pub(crate) fn model_call_allowed(
        &mut self,
        _turn: &TurnId,
    ) -> Result<Option<TurnCompleted>, Error> {
        Ok(None)
    }

    /// What a step's reply with no tool call leaves to do: the end-of-turn
    /// drain, then the `turn_end` hooks when nothing is waiting. `Some`
    /// ends the turn; `None` takes another step.
    pub(crate) fn replied(
        &mut self,
        turn: &TurnId,
        reply: &str,
    ) -> Result<Option<TurnCompleted>, Error> {
        let _ = reply;
        // Anything waiting continues the turn (`docs/loop.md`, "Ending a
        // turn"). A steer taken here waits for the next step; a prompt is
        // rejected.
        self.drain(turn)?;
        if self.queued.is_empty() {
            return Ok(Some(crate::ended(TurnOutcome::Completed, None)));
        }
        Ok(None)
    }

    /// Writes what the hooks held for the next step: `turn_end` messages as
    /// `steering_applied`, after `step_started` and the jobs notice, before
    /// the drain.
    pub(crate) fn write_held(&mut self, turn: &TurnId) -> Result<(), Error> {
        let _ = turn;
        Ok(())
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
