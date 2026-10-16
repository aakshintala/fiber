//! The `before_handoff` call site (`docs/extensions.md`, "The hook
//! points" and "When a hook fails"): a handoff has started, before the note
//! request. An extension's hook writes the note instead of the session's own
//! model; when none does, the loop makes its own note request. The loop
//! treats the note as opaque data, and an extension's name reaches it only
//! on the answer (`docs/architecture.md`, "The call rules").

use contract::events::{Event, HandoffTrigger};
use contract::hook::{BeforeHandoff, BeforeHandoffOutcome};
use contract::provider::Input;
use contract::{ErrorCode, TurnId};

use super::note::{Noted, failure};
use crate::{Error, Loop};

impl Loop {
    /// Asks the hooks for the handoff's note, and writes the notices they
    /// raised under `turn` with no action. Under a shutdown neither a hook
    /// nor a note request runs: the handoff completes `cancelled` at once,
    /// with hooks set or not. With no hooks set there is no note to write.
    /// A cancel that landed before the handoff check runs no hook, and one
    /// that landed while the hooks ran applies none of the answer: only the
    /// notices above are written. Never writes `handoff_completed`.
    pub(super) fn hook_note(
        &mut self,
        turn: &TurnId,
        trigger: HandoffTrigger,
        instructions: Option<&str>,
        conversation: &[Input],
    ) -> Result<Option<Noted>, Error> {
        // Under a shutdown no hook runs, so no note is written and no
        // note request follows (`docs/invocation.md`, "Shutdown").
        if self.shutting_down() {
            return Ok(Some(Noted::Cancelled));
        }
        let Some(hooks) = self.hooks.clone() else {
            return Ok(None);
        };
        // A cancel that landed before the handoff check runs no hook.
        if self.turn_cancelled() {
            return Ok(Some(Noted::Cancelled));
        }
        let answer = hooks.before_handoff(&BeforeHandoff {
            trigger,
            instructions,
            conversation,
            cancel: &*self.cancel,
        });
        for notice in answer.notices {
            self.write(&Event::Notice(notice), Some(turn), None)?;
        }
        // A cancel that landed while the hooks ran applies none of the
        // answer: only the notices above are written.
        if self.turn_cancelled() {
            return Ok(Some(Noted::Cancelled));
        }
        match answer.outcome {
            BeforeHandoffOutcome::NoNote => Ok(None),
            BeforeHandoffOutcome::Note { text, extension } => Ok(Some(Noted::Hook {
                note_text: text,
                extension,
            })),
            BeforeHandoffOutcome::Failed(hook_failure) => Ok(Some(Noted::Failed(failure(
                ErrorCode::HookFailed,
                &format!(
                    "The `before_handoff` hook of extension {} failed, so the handoff did not complete: {}",
                    hook_failure.extension, hook_failure.message
                ),
            )))),
        }
    }
}
