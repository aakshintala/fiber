//! What a driver, an extension or another session sends to a session's
//! loop (`docs/architecture.md`, "One inbox"). The loop owns the receiving
//! end; everyone else holds a sender, so this type lives here and no sender
//! depends on `loop`.

use std::fmt;

use crate::shapes::{ContentPart, Sender};

/// A message for the loop, carried by [`Delivery::Prompt`] or
/// [`Delivery::Steer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The message.
    pub content: Vec<ContentPart>,
    /// Where it came from.
    pub sender: Sender,
}

/// What an acknowledgement carries. `Ok(None)` accepts a command that
/// answers nothing; `Ok(Some)` accepts one that answers with a result;
/// `Err` rejects it. The loop never writes an answer to the log
/// (`docs/invocation.md`, "Driver commands").
pub type Answer = Result<Option<crate::events::CommandResult>, Rejection>;

/// Why a command was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The rejection code (`docs/invocation.md`, "Driver commands").
    pub code: crate::ErrorCode,
    /// Fiber's own sentence.
    pub message: String,
}

/// Called once with the command's answer. The loop calls it when it takes
/// the delivery, except a prompt that starts a turn, which it calls once
/// that turn's `turn_started` is written. Dropping an [`Ack`] uncalled is
/// the sender's concern: the loop stops on a log error without calling the
/// ones it has not taken.
pub struct Ack(pub Box<dyn FnOnce(Answer) + Send>);

impl fmt::Debug for Ack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ack(..)")
    }
}

/// What the loop's inbox carries (`docs/invocation.md`, "Driver commands").
#[derive(Debug)]
pub enum Delivery {
    /// Starts a turn. Rejected `busy` while one is running, and `closing`
    /// after `close`.
    Prompt(Message, Ack),
    /// A steering message. While a turn runs it joins that turn at the next
    /// step boundary; while the loop is idle it starts a turn.
    Steer(Message, Ack),
    /// Removes the steering message sent by the `steer` command `CommandId`
    /// names, when that message is still unapplied.
    SteerDrop(crate::CommandId, Ack),
    /// A person's answer to a pending approval.
    Reply(crate::commands::Reply, Ack),
    /// Accept no more prompts. The turn in flight finishes, then the loop
    /// exits.
    Close(Ack),
    /// Wakes a loop blocked on its inbox after an accepted `cancel`. It
    /// carries no ack and no meaning: every drain discards it, and an
    /// approval wait reads the cancel signal after it wakes
    /// (`docs/architecture.md`, "Cancellation").
    Cancelled,
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
