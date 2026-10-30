//! A person's or a driver's message before it is logged
//! (`docs/extensions.md`, "The hook points"): the `before_message` chain at
//! admission, before slash expansion, attending, queuing or logging. A
//! replacement is what the log, the model and the prompt history see; a
//! refusal or a blocking failure rejects the command at once.

use std::sync::Arc;
use std::sync::mpsc;

use contract::events::{Event, Notice, group_notices, grouped_lead};
use contract::hook::{BeforeMessage, BeforeMessageOutcome, Context, MessageSender, StateLine};
use contract::inbox::{Ack, Message};
use contract::shapes::ContentPart;
use contract::steering;
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
    pub(crate) context: Vec<Context>,
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
        sender: Option<MessageSender<'_>>,
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
            sender,
        });
        let state = hooks.take_change_state();
        self.drain_state(&hooks, turn)?;
        for notice in group_notices(answer.notices, grouped_lead) {
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
                    context,
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

    /// Runs the hook pass over the reader-admitted entry `id` carries and
    /// applies its outcome: `Passed` stages it in place (a replacement
    /// updates the entry and re-emits the queue) and reports the admitted
    /// message with its hook verdict for the caller to queue; any other
    /// answer removes the entry and emits the queue, plus a notice naming
    /// the code, extension and reason unless the loop is closing. A hook
    /// outcome for an id no longer present is discarded with no second
    /// emit (drop-wins). The already consumed ack makes every answer here
    /// silent.
    fn hook_pass(
        &mut self,
        id: &CommandId,
        turn: Option<&TurnId>,
    ) -> Result<Option<(Message, Hooked)>, Error> {
        let Some((content, sender)) = self.steering.unstaged(id) else {
            // Dropped between the caller's check and this pass.
            return Ok(None);
        };
        // The probe carries the verdict out: `admitted` answers it exactly
        // as it would the delivery's ack, then returns the outcome.
        let (probe_tx, probe_rx) = mpsc::channel();
        let probe = Ack(Box::new(move |answer| {
            let _sent = probe_tx.send(answer);
        }));
        let outcome = self.admitted(Message { content, sender }, None, probe, turn)?;
        let emit = inbox::TurnEmit {
            log: &self.log,
            turn: turn.cloned(),
        };
        match outcome {
            Some((message, hooked, _)) => {
                let staged = steering::Staged {
                    state: hooked.state.clone(),
                    changed_by: hooked.changed_by.clone(),
                    context: hooked.context.clone(),
                };
                if self
                    .steering
                    .stage(id, message.content.clone(), staged, &emit)
                {
                    Ok(Some((message, hooked)))
                } else {
                    // Dropped during the hook pass: discard, no second emit.
                    Ok(None)
                }
            }
            None => {
                // Refused, failed or shut down: admitted answered the
                // probe. The entry leaves with no notice when the loop
                // is closing: the session exit is the signal. A hook
                // outcome for an id no longer present is discarded with
                // no second emit (drop-wins): only a removal that
                // actually took the entry publishes the notice, so a
                // reader-side drop that won the race is never followed
                // by a stale rejection.
                if self.steering.take(id, &emit).is_some()
                    && let Ok(Err(rejection)) = probe_rx.try_recv()
                    && rejection.code != ErrorCode::Closing
                {
                    self.write(
                        &Event::Notice(Notice {
                            code: rejection.code,
                            message: rejection.message,
                            extension: None,
                        }),
                        turn,
                        None,
                    )?;
                }
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
        let Some((message, hooked, ack)) = self.admitted(message, None, ack, None)? else {
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

    /// Admits an idle `steer`: a reader-admitted one moves into the turn's
    /// input after the hook pass, keeping its id so a drop arriving in
    /// the same wait still finds it; a direct-inbox one is hooked,
    /// accepted and queued, as an unhooked steer is admitted today.
    pub(crate) fn take_steer_idle(
        &mut self,
        message: Message,
        ack: Ack,
        input: &mut TurnInput,
    ) -> Result<(), Error> {
        let id = message.sender.command_id.clone();
        match id {
            Some(id) if self.steering.unstaged(&id).is_some() => {
                let Some((message, hooked)) = self.hook_pass(&id, None)? else {
                    return Ok(());
                };
                let emit = inbox::TurnEmit {
                    log: &self.log,
                    turn: None,
                };
                if self.steering.move_out(&id, &emit).is_none() {
                    // Dropped between stage and move: drop-wins.
                    return Ok(());
                }
                self.attended();
                inbox::accept(ack);
                input.pieces.push(Queued::Steer(message, hooked));
                Ok(())
            }
            Some(id) if self.steering.is_admitted(&id) => {
                // Reader-removed or a duplicate delivery: the reader's
                // answer stands; this ack answers idempotently.
                inbox::accept(ack);
                Ok(())
            }
            _ => {
                let Some((message, hooked, ack)) = self.admitted(message, None, ack, None)? else {
                    return Ok(());
                };
                self.attended();
                inbox::accept(ack);
                input.pieces.push(Queued::Steer(message, hooked));
                Ok(())
            }
        }
    }

    /// Admits a `steer` while a turn runs: a reader-admitted one stages in
    /// place and queues its holder for the apply, in arrival order; a
    /// direct-inbox one is hooked, accepted, queued and emitted, exactly
    /// once through the shared queue.
    pub(crate) fn take_steer_running(
        &mut self,
        message: Message,
        ack: Ack,
        turn: &TurnId,
    ) -> Result<(), Error> {
        let id = message.sender.command_id.clone();
        match id {
            Some(id) if self.steering.unstaged(&id).is_some() => {
                // Reader-admitted: the hook pass stages it in place and
                // queues its holder for the apply, in arrival order; the
                // ack is already consumed.
                if let Some((message, hooked)) = self.hook_pass(&id, Some(turn))? {
                    self.attended();
                    inbox::accept(ack);
                    self.queued.push_back(Queued::Steer(message, hooked));
                }
                Ok(())
            }
            Some(id) if self.steering.is_admitted(&id) => {
                // Already dropped, or a duplicate delivery: the reader's
                // answer stands; this ack answers idempotently.
                inbox::accept(ack);
                Ok(())
            }
            _ => {
                let Some((message, hooked, ack)) = self.admitted(message, None, ack, Some(turn))?
                else {
                    return Ok(());
                };
                self.attended();
                inbox::accept(ack);
                match message.sender.command_id.clone() {
                    Some(id) => {
                        let staged = steering::Staged {
                            state: hooked.state.clone(),
                            changed_by: hooked.changed_by.clone(),
                            context: hooked.context.clone(),
                        };
                        let emit = inbox::TurnEmit {
                            log: &self.log,
                            turn: Some(turn.clone()),
                        };
                        // The id is absent from the shared queue, so the
                        // admit lands; a concurrent reader push wins by
                        // answering first, and this delivery stays a
                        // duplicate.
                        if self
                            .steering
                            .admit_hooked(
                                id,
                                message.content.clone(),
                                message.sender.clone(),
                                staged,
                                &emit,
                            )
                            .is_err()
                        {
                            return Ok(());
                        }
                        self.queued.push_back(Queued::Steer(message, hooked));
                        Ok(())
                    }
                    None => {
                        // Fiber's own message carries no id to key the
                        // queue by: it still applies, unannounced.
                        self.queued.push_back(Queued::Steer(message, hooked));
                        Ok(())
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "before_message_tests.rs"]
mod tests;
