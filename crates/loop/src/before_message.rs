//! A person's or a driver's message before it is logged
//! (`docs/extensions.md`, "The hook points"): the `before_message` chain at
//! admission, before slash expansion, attending, queuing or logging. A
//! replacement is what the log, the model and the prompt history see; a
//! refusal or a blocking failure rejects the command at once.

use std::sync::Arc;

use contract::events::{ContextAdded, Event};
use contract::hook::{BeforeMessage, BeforeMessageOutcome, StateLine};
use contract::inbox::{Ack, Message};
use contract::shapes::ContentPart;
use contract::{CommandId, ErrorCode, TurnId};

use crate::inbox::{self, TurnInput};
use crate::jobs::Queued;
use crate::{Error, Loop};

/// What [`Loop::on_rewrite`] reports to: a replaced prompt's command id
/// and its replacement, as the hook returned it, before slash expansion.
/// The door records it in the prompt history before the prompt's
/// acceptance (`docs/state.md`, "Prompt history").
pub type OnRewrite = Arc<dyn Fn(&CommandId, &[ContentPart]) + Send + Sync>;

/// What the `before_message` chain left on an admitted message: who
/// replaced its content, and the contexts its hooks added, in run order.
#[derive(Debug, Default)]
pub(crate) struct Hooked {
    pub(crate) state: Vec<StateLine>,
    /// The extensions whose hook returned `content`, in run order, each
    /// once; `Some` exactly when the message's content is a hook's.
    pub(crate) changed_by: Option<Vec<String>>,
    /// The contexts the hooks added, in run order.
    pub(crate) context: Vec<(ContextAdded, Vec<StateLine>)>,
}

impl Loop {
    /// Reports a replaced prompt's replacement to the door, which records
    /// it in the prompt history before the prompt's acceptance
    /// (`docs/state.md`, "Prompt history"). Like [`Loop::hooks`]: without
    /// it the door records what was sent.
    pub fn on_rewrite(mut self, rewritten: OnRewrite) -> Self {
        self.on_rewrite = Some(rewritten);
        self
    }

    /// Runs the `before_message` chain on `message` and writes its notices
    /// (`docs/extensions.md`, "When a hook fails"). With no hooks the
    /// message passes through untouched. With hooks set and a shutdown
    /// started the command is rejected `closing` and no hook runs
    /// (`docs/invocation.md`, "Shutdown"). Otherwise the chain's answer
    /// decides: `Passed` admits the message with its replacement and
    /// contexts, `Refused` and `Failed` reject the command and admit
    /// nothing. Never writes the message or its context, never calls
    /// `on_rewrite`, never marks the session attended.
    pub(crate) fn admitted(
        &mut self,
        message: Message,
        ack: Ack,
        turn: Option<&TurnId>,
    ) -> Result<Option<(Message, Hooked, Ack)>, Error> {
        let Some(hooks) = self.hooks.clone() else {
            return Ok(Some((message, Hooked::default(), ack)));
        };
        if self.shutting_down() {
            inbox::reject(ack, ErrorCode::Closing, inbox::CLOSING);
            return Ok(None);
        }
        let answer = hooks.before_message(&BeforeMessage {
            content: &message.content,
        });
        let state = hooks.take_change_state();
        self.drain_state(&hooks, turn)?;
        for notice in answer.notices {
            self.write(&Event::Notice(notice), turn, None)?;
        }
        match answer.outcome {
            BeforeMessageOutcome::Passed { replaced, context } => {
                let (content, changed_by) = match replaced {
                    Some(replaced) => (replaced.content, Some(replaced.changed_by)),
                    None => (message.content, None),
                };
                let hooked = Hooked {
                    state,
                    changed_by,
                    context: context
                        .into_iter()
                        .map(|added| {
                            (
                                ContextAdded {
                                    text: added.text,
                                    extension: added.extension,
                                    hook: "before_message".to_owned(),
                                },
                                added.state,
                            )
                        })
                        .collect(),
                };
                Ok(Some((
                    Message {
                        content,
                        sender: message.sender,
                    },
                    hooked,
                    ack,
                )))
            }
            BeforeMessageOutcome::Refused(refusal) => {
                inbox::reject(
                    ack,
                    ErrorCode::MessageRefused,
                    &format!(
                        "Extension {} refused the message: {}",
                        refusal.extension, refusal.reason
                    ),
                );
                Ok(None)
            }
            BeforeMessageOutcome::Failed(failure) => {
                inbox::reject(
                    ack,
                    ErrorCode::HookFailed,
                    &format!(
                        "The `before_message` hook of extension {} failed, \
                         so the message was not sent: {}",
                        failure.extension, failure.message
                    ),
                );
                Ok(None)
            }
        }
    }

    /// Admits an idle `prompt`: the hook, then the slash expansion that
    /// reads its replacement. A prompt the expansion rejects never reaches
    /// the door; a refused or blocked one never reaches the expansion.
    pub(crate) fn take_prompt(
        &mut self,
        message: Message,
        ack: Ack,
        input: &mut TurnInput,
    ) -> Result<(), Error> {
        let Some((message, hooked, ack)) = self.admitted(message, ack, None)? else {
            return Ok(());
        };
        // The replacement as the hook returned it, kept from before the
        // expansion rewrites it: the door records the pre-expansion form.
        let kept = hooked.changed_by.is_some().then(|| message.content.clone());
        match self.slash(message)? {
            Ok(expanded) => {
                if let (Some(kept), Some(id)) = (kept, expanded.sender.command_id.as_ref())
                    && let Some(on_rewrite) = self.on_rewrite.clone()
                {
                    (on_rewrite)(id, &kept);
                }
                self.attended();
                input.prompt_at.push(input.pieces.len());
                input.pieces.push(Queued::Steer(expanded, hooked));
                input.prompts.push(ack);
            }
            // A rejected prompt is not pushed and not attended: it moves
            // neither the turn nor the idle deadline
            // (`docs/invocation.md`, "Lifecycle").
            Err(rejection) => {
                (ack.0)(Err(rejection));
            }
        }
        Ok(())
    }

    /// Admits an idle `steer`: the hook, then attending, accepting and
    /// queuing, as an unhooked steer is admitted today.
    pub(crate) fn take_steer_idle(
        &mut self,
        message: Message,
        ack: Ack,
        input: &mut TurnInput,
    ) -> Result<(), Error> {
        let Some((message, hooked, ack)) = self.admitted(message, ack, None)? else {
            return Ok(());
        };
        self.attended();
        inbox::accept(ack);
        input.pieces.push(Queued::Steer(message, hooked));
        Ok(())
    }

    /// Admits a `steer` while a turn runs: the hook under the turn, then
    /// attending, accepting, queuing and announcing the queue.
    pub(crate) fn take_steer_running(
        &mut self,
        message: Message,
        ack: Ack,
        turn: &TurnId,
    ) -> Result<(), Error> {
        let Some((message, hooked, ack)) = self.admitted(message, ack, Some(turn))? else {
            return Ok(());
        };
        self.attended();
        inbox::accept(ack);
        self.queued.push_back(Queued::Steer(message, hooked));
        self.emit_queue(Some(turn))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "before_message_tests.rs"]
mod tests;
