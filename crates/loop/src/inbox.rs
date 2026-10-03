//! One drain for every delivery (`docs/architecture.md`, "One inbox").
//! The idle wait, a step boundary, the end-of-turn check and an approval
//! wait all admit a delivery through the same rules.

use contract::commands::Reply;
use contract::events::SteeringApplied;
use contract::inbox::{Ack, Delivery, Message, Rejection};
use contract::{CommandId, ErrorCode, RequestId, TurnId};

use crate::{Error, Loop};

/// A turn is running.
const BUSY: &str = "A turn is running; send `steer` to add to it.";

/// `close` has been taken.
const CLOSING: &str = "The session is closing and takes no new turn.";

/// A `steer_drop` names nothing still queued.
const STALE_STEER: &str = "That steering message was already applied, or was never queued.";

/// A `reply` names nothing pending.
const STALE_REPLY: &str = "That request is no longer pending.";

/// A `reply`'s keys do not fit the pending request.
pub(crate) const UNFIT_REPLY: &str = "That answer does not fit the pending request.";

/// What the idle drain collected. A prompt is accepted only while `messages`
/// is empty, so when `prompt` is set the prompt is the first message.
pub(crate) struct TurnInput {
    /// The turn's input, in arrival order.
    pub(crate) messages: Vec<Message>,
    /// Set when the first message is a prompt, accepted once `turn_started`
    /// is written. A steer was already accepted when taken.
    pub(crate) prompt: Option<Ack>,
}

/// What an approval wait should do with a delivery it took.
pub(crate) enum Waited {
    /// The delivery was answered. Keep waiting.
    Again,
    /// A reply that names the pending request. Not answered yet.
    Reply(Reply, Ack),
    /// `close` was taken. The pending approval is now unanswerable.
    Closed,
}

impl Loop {
    /// Blocks until a prompt or a steer is waiting to start a turn, and
    /// drains everything else already queued. `None` once `close` was taken
    /// with nothing to start, or every sender is gone
    /// (`docs/loop.md`, "Starting a turn").
    pub(crate) fn wait_for_turn(&mut self) -> Option<TurnInput> {
        if self.closing {
            return None;
        }
        loop {
            let first = self.inbox.recv().ok()?;
            let mut batch = vec![first];
            batch.extend(self.inbox.try_iter());
            let mut input = TurnInput {
                messages: Vec::new(),
                prompt: None,
            };
            for delivery in batch {
                self.admit_idle(delivery, &mut input);
            }
            if !input.messages.is_empty() {
                return Some(input);
            }
            if self.closing {
                return None;
            }
        }
    }

    /// Drains the inbox at a step boundary or the end-of-turn check. A steer
    /// taken here is left in `queued` until [`Self::apply_steering`].
    pub(crate) fn drain(&mut self) {
        let waiting: Vec<Delivery> = self.inbox.try_iter().collect();
        for delivery in waiting {
            self.admit_running(delivery);
        }
    }

    /// Logs every queued steer as `steering_applied`, in arrival order, and
    /// clears the queue (`docs/loop.md`, "One step").
    pub(crate) fn apply_steering(&mut self, turn: &TurnId) -> Result<(), Error> {
        while let Some(message) = self.queued.pop_front() {
            self.append(
                &contract::events::Event::SteeringApplied(SteeringApplied {
                    content: message.content,
                    sender: message.sender,
                    changed_by: None,
                }),
                turn,
                None,
            )?;
        }
        Ok(())
    }

    /// Admits `delivery` while the loop waits for a reply to `pending`.
    pub(crate) fn take_while_waiting(&mut self, pending: &RequestId, delivery: Delivery) -> Waited {
        match delivery {
            Delivery::Reply(reply, ack) if reply.request_id == *pending => {
                Waited::Reply(reply, ack)
            }
            Delivery::Close(ack) => {
                self.take_close(ack);
                Waited::Closed
            }
            other @ (Delivery::Prompt(..)
            | Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Reply(..)) => {
                self.admit_running(other);
                Waited::Again
            }
        }
    }

    /// `delivery` while the loop is still collecting a turn's input.
    fn admit_idle(&mut self, delivery: Delivery, input: &mut TurnInput) {
        match delivery {
            Delivery::Prompt(message, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else if !input.messages.is_empty() {
                    reject(ack, ErrorCode::Busy, BUSY);
                } else {
                    input.messages.push(message);
                    input.prompt = Some(ack);
                }
            }
            Delivery::Steer(message, ack) => {
                if self.closing && input.messages.is_empty() {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    accept(ack);
                    input.messages.push(message);
                }
            }
            Delivery::SteerDrop(id, ack) => {
                if drop_piece(input, &id) {
                    accept(ack);
                } else {
                    reject(ack, ErrorCode::StaleRequest, STALE_STEER);
                }
            }
            Delivery::Reply(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_REPLY),
            Delivery::Close(ack) => self.take_close(ack),
        }
    }

    /// `delivery` while a turn is in flight and nothing is pending.
    fn admit_running(&mut self, delivery: Delivery) {
        match delivery {
            Delivery::Prompt(_, ack) => {
                if self.closing {
                    reject(ack, ErrorCode::Closing, CLOSING);
                } else {
                    reject(ack, ErrorCode::Busy, BUSY);
                }
            }
            Delivery::Steer(message, ack) => {
                accept(ack);
                self.queued.push_back(message);
            }
            Delivery::SteerDrop(id, ack) => {
                if drop_queued(&mut self.queued, &id) {
                    accept(ack);
                } else {
                    reject(ack, ErrorCode::StaleRequest, STALE_STEER);
                }
            }
            Delivery::Reply(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_REPLY),
            Delivery::Close(ack) => self.take_close(ack),
        }
    }

    /// Accepts `close`. No later turn starts, and no later approval can be
    /// answered (`docs/permissions.md`, "Headless").
    fn take_close(&mut self, ack: Ack) {
        accept(ack);
        self.closing = true;
        self.answerable = false;
    }
}

/// Accepts a command that answers nothing.
pub(crate) fn accept(ack: Ack) {
    (ack.0)(Ok(None));
}

/// Rejects a command. The sentence is Fiber's own
/// (`docs/invocation.md`, "Driver commands").
pub(crate) fn reject(ack: Ack, code: ErrorCode, message: &str) {
    (ack.0)(Err(Rejection {
        code,
        message: message.to_owned(),
    }));
}

/// Removes the unapplied steer `id` names. The prompt, when present, is the
/// first message and is not a steer, so that index is skipped.
fn drop_piece(input: &mut TurnInput, id: &CommandId) -> bool {
    let skip = usize::from(input.prompt.is_some());
    input
        .messages
        .iter()
        .skip(skip)
        .position(|message| message.sender.command_id == *id)
        .map(|index| input.messages.remove(skip + index))
        .is_some()
}

/// Removes the unapplied steer `id` names from `queued`.
fn drop_queued(queued: &mut std::collections::VecDeque<Message>, id: &CommandId) -> bool {
    queued
        .iter()
        .position(|message| message.sender.command_id == *id)
        .map(|index| queued.remove(index))
        .is_some()
}
